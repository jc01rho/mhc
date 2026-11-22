use super::envelope::{exact, integer, text};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub kind: String,
    pub id: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Survivable {
    Synced,
    Roaming,
    /// A recovery-phrase key: its rp_id is the reserved phrase marker (#13330).
    Phrase,
}
impl Survivable {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Roaming => "roaming",
            Self::Phrase => "phrase",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryKey {
    pub credential_id: String,
    pub rp_id: String,
    pub enrollment_nonce: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
    pub survivable: Survivable,
}
#[derive(Debug, Clone)]
pub enum ParsedBody {
    Genesis {
        first_device: Principal,
        recovery_key: RecoveryKey,
        bootstrap: Value,
        custody_root: String,
        countersign: String,
        wraps_digest: String,
    },
    RecoveryKey {
        key: RecoveryKey,
        countersign: String,
        wraps_digest: String,
    },
    RecoveryKeyRemoved {
        credential_id: String,
    },
    Approval {
        principal: Principal,
        wraps_digest: String,
        commitment: String,
        joining_nonce: String,
    },
    Rotation {
        members: Vec<Principal>,
        wraps_digest: String,
        fork_resolution: Option<u64>,
    },
    RecoveryJoin {
        keyring_head: String,
        principal: Principal,
        credential_id: String,
    },
    RecoveryJoinVetoed {
        join_digest: String,
    },
    ForkResolution {
        members: Vec<Principal>,
        recovery_keys: Vec<RecoveryKey>,
        epoch: u64,
    },
}
impl ParsedBody {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Genesis { .. } => "genesis",
            Self::RecoveryKey { .. } => "recovery-key",
            Self::RecoveryKeyRemoved { .. } => "recovery-key-removed",
            Self::Approval { .. } => "approval",
            Self::Rotation { .. } => "rotation",
            Self::RecoveryJoin { .. } => "recovery-join",
            Self::RecoveryJoinVetoed { .. } => "recovery-join-vetoed",
            Self::ForkResolution { .. } => "fork-resolution",
        }
    }
}
pub fn parse_principal(v: &Value) -> Option<Principal> {
    if !exact(
        v,
        &["kind", "id", "sealing_public_key", "signing_public_key"],
    ) || v["kind"] != "device"
    {
        return None;
    }
    Some(Principal {
        kind: "device".into(),
        id: text(&v["id"])?.into(),
        sealing_public_key: text(&v["sealing_public_key"])?.into(),
        signing_public_key: text(&v["signing_public_key"])?.into(),
    })
}
const RECOVERY_KEYS: &[&str] = &[
    "credential_id",
    "rp_id",
    "enrollment_nonce",
    "sealing_public_key",
    "signing_public_key",
    "survivable",
];
fn read_recovery(v: &Value) -> Option<RecoveryKey> {
    let credential_id = text(&v["credential_id"])?;
    let rp_id = text(&v["rp_id"])?;
    let enrollment_nonce = text(&v["enrollment_nonce"])?;
    let sealing_public_key = text(&v["sealing_public_key"])?;
    let signing_public_key = text(&v["signing_public_key"])?;
    let survivable =
        super::recovery_kind::recovery_kind_of(v["survivable"].as_str(), rp_id, credential_id)?;
    Some(RecoveryKey {
        credential_id: credential_id.into(),
        rp_id: if survivable == Survivable::Phrase {
            super::recovery_kind::PHRASE_RECOVERY_RP_ID.into()
        } else {
            rp_id.into()
        },
        enrollment_nonce: enrollment_nonce.into(),
        sealing_public_key: sealing_public_key.into(),
        signing_public_key: signing_public_key.into(),
        survivable,
    })
}
pub fn parse_recovery(v: &Value) -> Option<RecoveryKey> {
    if !exact(v, RECOVERY_KEYS) {
        return None;
    }
    read_recovery(v)
}
fn shaped(body: &Value, fields: &[&str]) -> bool {
    let mut keys = vec!["kind", "account", "epoch"];
    keys.extend(fields);
    exact(body, &keys)
}
pub fn parse_body(b: &Value) -> Option<ParsedBody> {
    b["account"].as_str()?;
    integer(&b["epoch"], 1)?;
    match b["kind"].as_str()? {
        "genesis" => {
            if !shaped(
                b,
                &["bootstrap", "custody_root", "countersign", "wraps_digest"],
            ) {
                return None;
            }
            let boot = &b["bootstrap"];
            if !exact(
                boot,
                &["account", "custody_nonce", "first_device", "recovery_key"],
            ) || boot["account"] != b["account"]
            {
                return None;
            }
            text(&boot["custody_nonce"])?;
            Some(ParsedBody::Genesis {
                first_device: parse_principal(&boot["first_device"])?,
                recovery_key: parse_recovery(&boot["recovery_key"])?,
                bootstrap: boot.clone(),
                custody_root: text(&b["custody_root"])?.into(),
                countersign: text(&b["countersign"])?.into(),
                wraps_digest: text(&b["wraps_digest"])?.into(),
            })
        }
        "recovery-key" => {
            let mut keys = RECOVERY_KEYS.to_vec();
            keys.extend(["countersign", "wraps_digest"]);
            if !shaped(b, &keys) {
                return None;
            }
            Some(ParsedBody::RecoveryKey {
                key: read_recovery(b)?,
                countersign: text(&b["countersign"])?.into(),
                wraps_digest: text(&b["wraps_digest"])?.into(),
            })
        }
        "recovery-key-removed" => {
            if !shaped(b, &["credential_id"]) {
                return None;
            }
            Some(ParsedBody::RecoveryKeyRemoved {
                credential_id: text(&b["credential_id"])?.into(),
            })
        }
        "approval" => {
            let pairing = &b["pairing"];
            if !shaped(b, &["principal", "wraps_digest", "pairing"])
                || !exact(pairing, &["commitment", "joining_nonce", "approver_nonce"])
            {
                return None;
            }
            text(&pairing["approver_nonce"])?;
            Some(ParsedBody::Approval {
                principal: parse_principal(&b["principal"])?,
                wraps_digest: text(&b["wraps_digest"])?.into(),
                commitment: text(&pairing["commitment"])?.into(),
                joining_nonce: text(&pairing["joining_nonce"])?.into(),
            })
        }
        "rotation" => {
            if !shaped(b, &["members", "wraps_digest", "fork_resolution"]) {
                return None;
            }
            let members = b["members"]
                .as_array()?
                .iter()
                .map(parse_principal)
                .collect::<Option<Vec<_>>>()?;
            if members.is_empty() {
                return None;
            }
            Some(ParsedBody::Rotation {
                members,
                wraps_digest: text(&b["wraps_digest"])?.into(),
                fork_resolution: if b["fork_resolution"].is_null() {
                    None
                } else {
                    Some(integer(&b["fork_resolution"], 1)?)
                },
            })
        }
        "recovery-join" => {
            if !shaped(b, &["keyring_head", "principal", "credential_id"]) {
                return None;
            }
            Some(ParsedBody::RecoveryJoin {
                keyring_head: text(&b["keyring_head"])?.into(),
                principal: parse_principal(&b["principal"])?,
                credential_id: text(&b["credential_id"])?.into(),
            })
        }
        "recovery-join-vetoed" => {
            if !shaped(b, &["join_digest"]) {
                return None;
            }
            Some(ParsedBody::RecoveryJoinVetoed {
                join_digest: text(&b["join_digest"])?.into(),
            })
        }
        "fork-resolution" => {
            let state = &b["state"];
            if !shaped(
                b,
                &[
                    "fork_set",
                    "evidence",
                    "continuation",
                    "state",
                    "merged_digest",
                ],
            ) || !exact(state, &["members", "recovery_keys", "epoch"])
            {
                return None;
            }
            Some(ParsedBody::ForkResolution {
                members: state["members"]
                    .as_array()?
                    .iter()
                    .map(parse_principal)
                    .collect::<Option<_>>()?,
                recovery_keys: state["recovery_keys"]
                    .as_array()?
                    .iter()
                    .map(parse_recovery)
                    .collect::<Option<_>>()?,
                epoch: integer(&state["epoch"], 1)?,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::crypto::recovery_kind::{PHRASE_RECOVERY_RP_ID, is_phrase_key};
    use crate::sync::crypto::testing::{fixtures, row};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::json;

    fn phrase_key() -> Value {
        json!({
            "credential_id": URL_SAFE_NO_PAD.encode([1u8; 16]),
            "rp_id": PHRASE_RECOVERY_RP_ID,
            "enrollment_nonce": URL_SAFE_NO_PAD.encode([2u8; 32]),
            "sealing_public_key": URL_SAFE_NO_PAD.encode([3u8; 32]),
            "signing_public_key": URL_SAFE_NO_PAD.encode([4u8; 32]),
            "survivable": "phrase",
        })
    }

    fn genesis_with(key: Value) -> Value {
        let mut body = row(&fixtures(), "genesis")["body"].clone();
        body["bootstrap"]["recovery_key"] = key;
        body
    }

    fn recovery_key_with(key: Value) -> Value {
        let mut body = row(&fixtures(), "recovery_key")["body"].clone();
        for (field, value) in key.as_object().expect("key object") {
            body[field] = value.clone();
        }
        body
    }

    fn fork_state_with(key: Value) -> Value {
        let mut body = row(&fixtures(), "fork_resolution")["body"].clone();
        body["state"]["recovery_keys"]
            .as_array_mut()
            .expect("recovery keys")
            .push(key);
        body
    }

    fn recovery_keys_of(body: &Value) -> Vec<RecoveryKey> {
        match parse_body(body).expect("body parses") {
            ParsedBody::Genesis { recovery_key, .. } => vec![recovery_key],
            ParsedBody::RecoveryKey { key, .. } => vec![key],
            ParsedBody::ForkResolution { recovery_keys, .. } => recovery_keys,
            other => panic!("unexpected body kind: {}", other.kind()),
        }
    }

    #[test]
    fn phrase_key_parses_across_every_keyring_body_site() {
        assert_eq!(PHRASE_RECOVERY_RP_ID, "omo-sync/v1/recovery-phrase");
        let key = phrase_key();
        let expected_id = key["credential_id"].as_str().expect("id").to_string();
        for body in [
            genesis_with(key.clone()),
            recovery_key_with(key.clone()),
            fork_state_with(key.clone()),
        ] {
            let parsed = recovery_keys_of(&body);
            let found = parsed
                .iter()
                .find(|candidate| candidate.credential_id == expected_id)
                .expect("phrase key present");
            assert_eq!(found.survivable, Survivable::Phrase);
            assert_eq!(found.rp_id, PHRASE_RECOVERY_RP_ID);
            assert!(is_phrase_key(found));
            assert_eq!(parsed.iter().filter(|k| is_phrase_key(k)).count(), 1);
        }
    }

    #[test]
    fn passkey_kinds_still_parse_unchanged_at_every_site() {
        let mut passkey = row(&fixtures(), "genesis")["body"]["bootstrap"]["recovery_key"].clone();
        passkey["credential_id"] = json!("cGFzc2tleS1yZWdyZXNzaW9u");
        for body in [
            genesis_with(passkey.clone()),
            recovery_key_with(passkey.clone()),
            fork_state_with(passkey),
        ] {
            let parsed = recovery_keys_of(&body);
            assert!(!parsed.iter().any(is_phrase_key));
            let found = parsed
                .iter()
                .find(|k| k.credential_id == "cGFzc2tleS1yZWdyZXNzaW9u")
                .expect("passkey present");
            assert_eq!(found.survivable, Survivable::Synced);
        }
    }

    #[test]
    fn refused_kind_pairs_and_credential_lengths_are_rejected_at_every_boundary() {
        let refused_pairs = [
            json!({"survivable": "phrase", "rp_id": "recovery.omo.dev"}),
            json!({"survivable": "synced", "rp_id": PHRASE_RECOVERY_RP_ID}),
            json!({"survivable": "roaming", "rp_id": PHRASE_RECOVERY_RP_ID}),
            json!({"survivable": "synced", "rp_id": "omo-sync/v2/other"}),
            json!({"survivable": "roaming", "rp_id": "omo-sync/"}),
            json!({"survivable": "paper", "rp_id": PHRASE_RECOVERY_RP_ID}),
        ];
        for pair in refused_pairs {
            let mut key = phrase_key();
            for (field, value) in pair.as_object().expect("pair") {
                key[field] = value.clone();
            }
            assert!(parse_recovery(&key).is_none(), "pair {pair} parsed");
            assert!(parse_body(&genesis_with(key.clone())).is_none());
            assert!(parse_body(&recovery_key_with(key.clone())).is_none());
            assert!(parse_body(&fork_state_with(key)).is_none());
        }
        for len in [15usize, 17, 32] {
            let mut key = phrase_key();
            key["credential_id"] = json!(URL_SAFE_NO_PAD.encode(vec![7u8; len]));
            assert!(parse_recovery(&key).is_none(), "{len}-byte id parsed");
            assert!(parse_body(&genesis_with(key.clone())).is_none());
            assert!(parse_body(&recovery_key_with(key.clone())).is_none());
            assert!(parse_body(&fork_state_with(key)).is_none());
        }
    }
}
