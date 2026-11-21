//! `<state dir>/relayd.json`: the account label, this device's peer listener, and the peers it trusts.
//!
//! ```json
//! {"account": "home", "listen": "100.64.0.2:7810",
//!  "peers": [{"name": "laptop", "address": "laptop.tail1234.ts.net:7810", "public_key": "<base64url>"}]}
//! ```
//!
//! The peer link carries plain text, so `listen` and every peer address must be a loopback or tailnet
//! host (`maho_service_endpoints::check_peer_host`); anything else is refused at start.

use std::path::Path;

use maho_service_endpoints::check_peer_host;
use serde::{Deserialize, Serialize};

use crate::identity::{decode_public_key, device_id_of};

pub const CONFIG_FILE: &str = "relayd.json";
pub const DEFAULT_ACCOUNT: &str = "local";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerConfig {
    pub name: String,
    /// `host:port` (`[v6]:port` for IPv6) of the peer's listener.
    pub address: String,
    /// The peer's Ed25519 public key, base64url.
    pub public_key: String,
}

impl PeerConfig {
    /// The device id the peer proves, derived from its public key.
    pub fn device_id(&self) -> Option<String> {
        decode_public_key(&self.public_key).map(|key| device_id_of(&key.to_bytes()))
    }
}

fn default_account() -> String {
    DEFAULT_ACCOUNT.to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaydConfig {
    #[serde(default = "default_account")]
    pub account: String,
    /// This device's name; the host name when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The peer listener; no peer link is served when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
}

impl Default for RelaydConfig {
    fn default() -> Self {
        Self {
            account: default_account(),
            name: None,
            listen: None,
            peers: Vec::new(),
        }
    }
}

/// The host part of `host:port` or `[v6]:port`.
pub fn address_host(address: &str) -> Option<&str> {
    let address = address.trim();
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host);
    }
    address.rsplit_once(':').map(|(host, _)| host)
}

impl RelaydConfig {
    /// The config in `state_dir`; the defaults when there is none.
    pub fn load(state_dir: &Path) -> Result<Self, String> {
        let path = state_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(state_dir).map_err(|error| error.to_string())?;
        std::fs::write(state_dir.join(CONFIG_FILE), text + "\n").map_err(|error| error.to_string())
    }

    /// Refuses a listener or a peer the plain-text link must not reach, and malformed keys.
    pub fn validate(&self) -> Result<(), String> {
        if self.account.trim().is_empty() || self.account.contains('\n') {
            return Err("relayd.json: account must be a non-empty single line".into());
        }
        if let Some(listen) = &self.listen {
            let host = address_host(listen)
                .ok_or_else(|| format!("relayd.json: listen {listen} is not host:port"))?;
            check_peer_host(host)
                .map_err(|refusal| format!("relayd.json: listen {listen}: {refusal}"))?;
        }
        for peer in &self.peers {
            let host = address_host(&peer.address).ok_or_else(|| {
                format!("relayd.json: peer {} address is not host:port", peer.name)
            })?;
            check_peer_host(host)
                .map_err(|refusal| format!("relayd.json: peer {}: {refusal}", peer.name))?;
            if peer.device_id().is_none() {
                return Err(format!(
                    "relayd.json: peer {} has no valid public_key",
                    peer.name
                ));
            }
        }
        Ok(())
    }

    /// The peer with this device id, name (case-insensitive) or unique id prefix.
    pub fn find_peer(&self, device: &str) -> Option<&PeerConfig> {
        if let Some(peer) = self
            .peers
            .iter()
            .find(|peer| peer.device_id().as_deref() == Some(device))
        {
            return Some(peer);
        }
        let named: Vec<&PeerConfig> = self
            .peers
            .iter()
            .filter(|peer| peer.name.eq_ignore_ascii_case(device))
            .collect();
        if let [peer] = named.as_slice() {
            return Some(peer);
        }
        let prefixed: Vec<&PeerConfig> = self
            .peers
            .iter()
            .filter(|peer| peer.device_id().is_some_and(|id| id.starts_with(device)))
            .collect();
        match prefixed.as_slice() {
            [peer] if !device.is_empty() => Some(peer),
            _ => None,
        }
    }
}
