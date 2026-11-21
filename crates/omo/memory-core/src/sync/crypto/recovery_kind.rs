use super::parse::{RecoveryKey, Survivable};
use super::signatures::decode_b64;

/// The rp_id every recovery-phrase key carries in place of a WebAuthn relying party.
pub const PHRASE_RECOVERY_RP_ID: &str = "omo-sync/v1/recovery-phrase";

/// The rp_id namespace reserved for OmO-defined recovery kinds; no passkey and no PRF salt may use it.
pub const RESERVED_RP_PREFIX: &str = "omo-sync/";

const PHRASE_CREDENTIAL_BYTES: usize = 16;

/// The recovery-key kind rules of contract sec 1, shared by every boundary that admits a key: the
/// survivable value is one of three; it is `Phrase` exactly when the rp_id is the phrase marker; a
/// phrase credential id is 16 bytes; a passkey rp_id never lies in the reserved `omo-sync/` namespace.
pub fn recovery_kind_of(survivable: &str, rp_id: &str, credential_id: &str) -> Option<Survivable> {
    match survivable {
        "phrase" => {
            let marker = rp_id == PHRASE_RECOVERY_RP_ID;
            let sixteen = decode_b64(credential_id, Some(PHRASE_CREDENTIAL_BYTES)).is_some();
            (marker && sixteen).then_some(Survivable::Phrase)
        }
        "synced" | "roaming" => {
            if rp_id.starts_with(RESERVED_RP_PREFIX) {
                return None;
            }
            Some(if survivable == "synced" {
                Survivable::Synced
            } else {
                Survivable::Roaming
            })
        }
        _ => None,
    }
}

pub fn is_phrase_key(key: &RecoveryKey) -> bool {
    key.survivable == Survivable::Phrase
}
