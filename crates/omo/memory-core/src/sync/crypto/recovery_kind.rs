//! The recovery-key kind rules of contract section 1, shared by every boundary that admits a key:
//! the survivable value is one of three; it is `phrase` exactly when the rp_id is the phrase marker;
//! a phrase credential id is 16 bytes; a passkey rp_id never lies in the reserved `omo-sync/`
//! namespace (#13330).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

pub use crate::sync::crypto::parse::Survivable;

pub const PHRASE_RECOVERY_RP_ID: &str = "omo-sync/v1/recovery-phrase";

/// The rp_id namespace reserved for OmO-defined recovery kinds; no passkey and no PRF salt may use it.
pub const RESERVED_RP_PREFIX: &str = "omo-sync/";

const PHRASE_CREDENTIAL_BYTES: usize = 16;

fn decodes_to(text: &str, length: usize) -> bool {
    URL_SAFE_NO_PAD
        .decode(text)
        .is_ok_and(|bytes| bytes.len() == length)
}

/// The recovery-key kind a served record names, or `None` when its three values break a rule. A
/// `phrase` key is reported with the reserved rp_id, whatever the row carried.
pub fn recovery_kind_of(
    survivable: Option<&str>,
    rp_id: &str,
    credential_id: &str,
) -> Option<Survivable> {
    match survivable {
        Some("phrase") => (rp_id == PHRASE_RECOVERY_RP_ID
            && decodes_to(credential_id, PHRASE_CREDENTIAL_BYTES))
        .then_some(Survivable::Phrase),
        Some("synced") => (!rp_id.starts_with(RESERVED_RP_PREFIX)).then_some(Survivable::Synced),
        Some("roaming") => (!rp_id.starts_with(RESERVED_RP_PREFIX)).then_some(Survivable::Roaming),
        _ => None,
    }
}

pub fn is_phrase_key(key: &crate::sync::crypto::parse::RecoveryKey) -> bool {
    key.survivable == Survivable::Phrase
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDENTIAL: &str = "AAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn the_phrase_kind_needs_the_reserved_rp_id_and_a_16_byte_credential() {
        assert_eq!(
            recovery_kind_of(Some("phrase"), PHRASE_RECOVERY_RP_ID, CREDENTIAL),
            Some(Survivable::Phrase)
        );
        assert_eq!(
            recovery_kind_of(Some("phrase"), "example.com", CREDENTIAL),
            None
        );
        assert_eq!(
            recovery_kind_of(Some("phrase"), PHRASE_RECOVERY_RP_ID, "short"),
            None
        );
        assert_eq!(
            recovery_kind_of(Some("phrase"), PHRASE_RECOVERY_RP_ID, "AAAA"),
            None
        );
    }

    #[test]
    fn a_passkey_kind_never_takes_the_reserved_namespace() {
        assert_eq!(
            recovery_kind_of(Some("synced"), "example.com", "any"),
            Some(Survivable::Synced)
        );
        assert_eq!(
            recovery_kind_of(Some("roaming"), "example.com", "any"),
            Some(Survivable::Roaming)
        );
        assert_eq!(recovery_kind_of(Some("synced"), "omo-sync/x", "any"), None);
        assert_eq!(recovery_kind_of(Some("roaming"), "omo-sync/x", "any"), None);
        assert_eq!(recovery_kind_of(Some("other"), "example.com", "any"), None);
        assert_eq!(recovery_kind_of(None, "example.com", "any"), None);
    }
}
