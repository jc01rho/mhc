//! The account-wide sessions and deliveries ops (`session.locate`, `session.read`,
//! `session.command_part`, `sessions.events.subscribe`, `deliveries.serve`, ...) go over the
//! agentd path: a peer in `relayd.json` must carry an `agentd` object for mhc to reach it there.
//! A caller aimed at a peer without one gets [`AGENTD_REFUSAL`]: the relay itself deliberately
//! refuses those ten names, so there is no silent relay fallback for them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Machine-readable cause for a remote op aimed at a peer with no `agentd` entry.
pub const AGENTD_ENTRY_REQUIRED: &str = "agentd_entry_required";

/// The refusal such a caller sees: cause, the fix, and the code it keeps from the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentdRefusal {
    pub code: &'static str,
    pub reason: &'static str,
    pub fix: &'static str,
}

impl AgentdRefusal {
    pub fn to_json(&self, device: &str) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("code".to_owned(), Value::from(self.code)),
            ("reason".to_owned(), Value::from(self.reason)),
            ("fix".to_owned(), Value::from(self.fix)),
            ("device".to_owned(), Value::from(device)),
        ]))
    }
}

pub const AGENTD_REFUSAL: AgentdRefusal = AgentdRefusal {
    code: "unsupported",
    reason: AGENTD_ENTRY_REQUIRED,
    fix: "add an `agentd` object (address, port, tokenFile) to this peer in relayd.json",
};

/// Where one device's agentd listens, and the token file that admits this client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentdPeer {
    /// The peer's name in `relayd.json`, which is also the device name mhc addresses it by.
    pub device: String,
    pub address: String,
    pub port: u16,
    pub token_file: PathBuf,
}

impl AgentdPeer {
    /// `host:port` of this peer's daemon, `[v6]:port` for an IPv6 address.
    pub fn socket_address(&self) -> String {
        if self.address.contains(':') && !self.address.starts_with('[') {
            format!("[{}]:{}", self.address, self.port)
        } else {
            format!("{}:{}", self.address, self.port)
        }
    }
}

/// The token this peer admits, read from its token file; a missing or empty file is no token at all.
pub fn read_token(peer: &AgentdPeer) -> Result<String, String> {
    let text = std::fs::read_to_string(&peer.token_file)
        .map_err(|error| format!("{}: {error}", peer.token_file.display()))?;
    let token = text.trim().to_owned();
    if token.is_empty() {
        return Err(format!(
            "{}: the agentd token file is empty",
            peer.token_file.display()
        ));
    }
    Ok(token)
}

/// Every peer in `relayd.json` that carries an `agentd` object, by device name.
pub fn peers_from_config(config: &Value, state_dir: &Path) -> Vec<AgentdPeer> {
    let Some(Value::Array(peers)) = config.get("peers") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for peer in peers {
        let Some(device) = peer.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(agentd) = peer.get("agentd") else {
            continue;
        };
        let Some(address) = agentd.get("address").and_then(Value::as_str) else {
            continue;
        };
        let port = agentd
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok());
        let Some(port) = port else {
            continue;
        };
        let token_file = agentd
            .get("tokenFile")
            .and_then(Value::as_str)
            .map(|file| {
                let path = PathBuf::from(file);
                if path.is_absolute() {
                    path
                } else {
                    state_dir.join(path)
                }
            })
            .unwrap_or_else(|| state_dir.join("agentd-tokens").join(device));
        found.push(AgentdPeer {
            device: device.to_owned(),
            address: address.trim().to_owned(),
            port,
            token_file,
        });
    }
    found
}

/// The peers named in `<state dir>/relayd.json`; an unreadable or unparsable file names none.
pub fn peers_in(state_dir: &Path) -> Vec<AgentdPeer> {
    let Ok(text) = std::fs::read_to_string(state_dir.join("relayd.json")) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    peers_from_config(&config, state_dir)
}

/// The configured peers by device name.
pub fn peer_map(state_dir: &Path) -> BTreeMap<String, AgentdPeer> {
    peers_in(state_dir)
        .into_iter()
        .map(|peer| (peer.device.clone(), peer))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(peers: Value) -> Value {
        serde_json::json!({ "account": "home", "peers": peers })
    }

    #[test]
    fn a_peer_with_an_agentd_object_is_reachable_and_one_without_keeps_the_relayd_path() {
        let dir = Path::new("/state");
        let peers = peers_from_config(
            &config(serde_json::json!([
                { "name": "mac-mini", "address": "100.64.0.2:7810", "public_key": "k",
                  "agentd": { "address": "100.64.0.2", "port": 7811, "tokenFile": "/keys/mini" } },
                { "name": "laptop", "address": "100.64.0.3:7810", "public_key": "k" }
            ])),
            dir,
        );
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].device, "mac-mini");
        assert_eq!(peers[0].socket_address(), "100.64.0.2:7811");
        assert_eq!(peers[0].token_file, PathBuf::from("/keys/mini"));
        let refusal = AGENTD_REFUSAL.to_json("laptop");
        assert_eq!(refusal["code"], json!("unsupported"));
        assert_eq!(refusal["reason"], json!(AGENTD_ENTRY_REQUIRED));
        assert!(
            refusal["fix"]
                .as_str()
                .is_some_and(|fix| fix.contains("relayd.json"))
        );
        assert_eq!(refusal["device"], json!("laptop"));
    }

    #[test]
    fn a_relative_token_file_is_read_inside_the_state_dir_and_an_absent_one_has_a_default() {
        let dir = Path::new("/state");
        let peers = peers_from_config(
            &config(serde_json::json!([
                { "name": "a", "agentd": { "address": "10.0.0.1", "port": 1, "tokenFile": "tokens/a" } },
                { "name": "b", "agentd": { "address": "10.0.0.2", "port": 2 } }
            ])),
            dir,
        );
        assert_eq!(peers[0].token_file, PathBuf::from("/state/tokens/a"));
        assert_eq!(peers[1].token_file, PathBuf::from("/state/agentd-tokens/b"));
    }

    #[test]
    fn a_peer_missing_an_address_or_a_port_is_not_a_peer() {
        let peers = peers_from_config(
            &config(serde_json::json!([
                { "name": "no-port", "agentd": { "address": "10.0.0.1" } },
                { "name": "no-address", "agentd": { "port": 7 } },
                { "name": "bad-port", "agentd": { "address": "10.0.0.1", "port": 70000 } }
            ])),
            Path::new("/state"),
        );
        assert!(peers.is_empty());
    }

    #[test]
    fn an_ipv6_address_is_bracketed_and_an_ipv4_one_is_not() {
        let peer = |address: &str| AgentdPeer {
            device: "d".to_owned(),
            address: address.to_owned(),
            port: 9,
            token_file: PathBuf::from("/t"),
        };
        assert_eq!(peer("fd7a::1").socket_address(), "[fd7a::1]:9");
        assert_eq!(peer("[fd7a::1]").socket_address(), "[fd7a::1]:9");
        assert_eq!(peer("100.64.0.2").socket_address(), "100.64.0.2:9");
    }

    #[test]
    fn a_token_file_that_is_missing_or_blank_admits_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut peer = AgentdPeer {
            device: "d".to_owned(),
            address: "127.0.0.1".to_owned(),
            port: 1,
            token_file: dir.path().join("token"),
        };
        assert!(read_token(&peer).is_err());
        std::fs::write(&peer.token_file, "  \n").expect("blank token");
        assert!(read_token(&peer).is_err());
        std::fs::write(&peer.token_file, " secret-token\n").expect("token");
        assert_eq!(read_token(&peer).as_deref(), Ok("secret-token"));
        peer.token_file = dir.path().join("missing");
        assert!(read_token(&peer).is_err());
    }

    #[test]
    fn a_state_dir_without_a_config_names_no_peers() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(peers_in(dir.path()).is_empty());
        std::fs::write(dir.path().join("relayd.json"), "{not json").expect("broken config");
        assert!(peers_in(dir.path()).is_empty());
        std::fs::write(
            dir.path().join("relayd.json"),
            serde_json::json!({"peers": [{"name": "a", "agentd": {"address": "h", "port": 1}}]})
                .to_string(),
        )
        .expect("config");
        assert_eq!(peer_map(dir.path()).keys().collect::<Vec<_>>(), vec!["a"]);
    }
}
