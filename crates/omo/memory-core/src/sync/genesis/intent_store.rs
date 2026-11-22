//! Where one account/principal keeps its genesis intent: `<pinDir>/genesis-intent/<sha256 of
//! [account, principal id]>`, with its lock and phrase markers beside it (#13335). The intent itself
//! is a public, exact signed request; publishing it is a rename of a complete directory, so it can
//! never overwrite another intent, and a crash leaves the old intent or the new one.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::bridge::GenesisEnableError;
use super::phrase_marker::PhraseGenesisPlace;
use crate::fs::resilient;
use crate::journal::sync_journal_directory;
use crate::sync::crypto::signatures::sha256;

/// The directory one account/principal keeps its genesis intent in.
pub fn genesis_intent_location(pin_dir: &Path, account: &str, principal_id: &str) -> PathBuf {
    let digest = sha256(
        serde_json::to_vec(&json!([account, principal_id]))
            .unwrap_or_default()
            .as_slice(),
    );
    let key: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    pin_dir.join("genesis-intent").join(key)
}

/// The phrase marker place beside an intent location.
pub fn phrase_marker_place(location: &Path, account: &str) -> PhraseGenesisPlace {
    PhraseGenesisPlace {
        intent_location: location.to_path_buf(),
        account: account.to_string(),
    }
}

fn intent_file(location: &Path) -> PathBuf {
    location.join("intent.json")
}

pub fn load_genesis_intent(location: &Path) -> Result<Option<Value>, GenesisEnableError> {
    let raw = match resilient::read_to_string(&intent_file(location)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A directory without its intent is a corrupt intent, not an absent one.
            return match resilient::metadata(location) {
                Ok(_) => Err(GenesisEnableError::CorruptIntent),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(GenesisEnableError::CorruptIntent),
            };
        }
        Err(_) => return Err(GenesisEnableError::CorruptIntent),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| GenesisEnableError::CorruptIntent)
}

fn digest_of(intent: &Value) -> Option<&str> {
    intent["genesisDigest"]
        .as_str()
        .or_else(|| intent["genesis_digest"].as_str())
}

/// Publishes `intent` at `location` atomically, then reads it back: a stored intent that does not
/// read back as the one written is a corrupt intent.
pub fn store_genesis_intent(location: &Path, intent: &Value) -> Result<(), GenesisEnableError> {
    let parent = location
        .parent()
        .ok_or(GenesisEnableError::MissingConfiguration)?;
    resilient::create_dir_all(parent).map_err(|_| GenesisEnableError::MissingConfiguration)?;
    let suffix = crate::support::random::random_id();
    let staging = parent.join(format!(
        "{}.{suffix}.tmp",
        location
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let written = (|| -> std::io::Result<()> {
        resilient::create_dir_all(&staging)?;
        let text = serde_json::to_vec(intent).map_err(std::io::Error::other)?;
        resilient::write(&intent_file(&staging), &text)?;
        if let Ok(handle) = std::fs::File::open(intent_file(&staging)) {
            let _ = handle.sync_all();
        }
        sync_journal_directory(&staging)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if resilient::exists(location) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a genesis intent is already stored",
            ));
        }
        resilient::rename(&staging, location)
    })();
    let _ = resilient::remove_dir_all(&staging);
    written.map_err(|_| GenesisEnableError::CorruptIntent)?;
    let _ = sync_journal_directory(parent);
    match load_genesis_intent(location) {
        Ok(Some(stored))
            if digest_of(&stored) == digest_of(intent) && digest_of(intent).is_some() =>
        {
            Ok(())
        }
        _ => Err(GenesisEnableError::CorruptIntent),
    }
}

/// Retires the stored intent by renaming it aside first, so a crash cannot half-delete it.
pub fn retire_genesis_intent(location: &Path) -> Result<(), GenesisEnableError> {
    if !resilient::exists(location) {
        return Ok(());
    }
    let retired = PathBuf::from(format!(
        "{}.retired-{}",
        location.to_string_lossy(),
        crate::support::random::random_id()
    ));
    resilient::rename(location, &retired).map_err(|_| GenesisEnableError::CorruptIntent)?;
    let _ = resilient::remove_dir_all(&retired);
    if let Some(parent) = location.parent() {
        let _ = sync_journal_directory(parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(digest: &str) -> Value {
        json!({"genesisDigest": digest, "account": "acct", "bootstrap": {}})
    }

    #[test]
    fn an_intent_is_published_atomically_and_read_back() -> Result<(), GenesisEnableError> {
        let pins = tempfile::tempdir().expect("temp");
        let location = genesis_intent_location(pins.path(), "acct", "device-1");
        assert_eq!(load_genesis_intent(&location)?, None);
        store_genesis_intent(&location, &intent("root-1"))?;
        assert_eq!(load_genesis_intent(&location)?, Some(intent("root-1")));
        let other = genesis_intent_location(pins.path(), "acct", "device-2");
        assert_ne!(location, other);
        Ok(())
    }

    #[test]
    fn a_second_store_never_overwrites_a_published_intent() -> Result<(), GenesisEnableError> {
        let pins = tempfile::tempdir().expect("temp");
        let location = genesis_intent_location(pins.path(), "acct", "device-1");
        store_genesis_intent(&location, &intent("root-1"))?;
        assert_eq!(
            store_genesis_intent(&location, &intent("root-2")),
            Err(GenesisEnableError::CorruptIntent)
        );
        assert_eq!(load_genesis_intent(&location)?, Some(intent("root-1")));
        retire_genesis_intent(&location)?;
        assert_eq!(load_genesis_intent(&location)?, None);
        Ok(())
    }

    #[test]
    fn a_directory_without_its_intent_is_corrupt() {
        let pins = tempfile::tempdir().expect("temp");
        let location = genesis_intent_location(pins.path(), "acct", "device-1");
        std::fs::create_dir_all(&location).expect("dir");
        assert_eq!(
            load_genesis_intent(&location),
            Err(GenesisEnableError::CorruptIntent)
        );
    }

    #[test]
    fn a_stored_phrase_intent_round_trips_and_corrupt_json_is_refused()
    -> Result<(), GenesisEnableError> {
        let pins = tempfile::tempdir().expect("temp");
        let location = genesis_intent_location(pins.path(), "acct", "device-1");
        let phrase_intent = json!({
            "genesisDigest": "phrase-root-1",
            "account": "acct",
            "request": {
                "record": {
                    "kind": "genesis",
                    "bootstrap": {
                        "recovery_key": {
                            "rp_id": crate::sync::crypto::recovery_kind::PHRASE_RECOVERY_RP_ID,
                            "survivable": "phrase",
                        }
                    }
                }
            }
        });
        store_genesis_intent(&location, &phrase_intent)?;
        let loaded = load_genesis_intent(&location)?.expect("loaded");
        assert_eq!(
            loaded["request"]["record"]["bootstrap"]["recovery_key"]["survivable"],
            "phrase"
        );
        resilient::write(&intent_file(&location), b"{broken").expect("corrupt write");
        assert_eq!(
            load_genesis_intent(&location),
            Err(GenesisEnableError::CorruptIntent)
        );
        let without_digest_loc = genesis_intent_location(pins.path(), "acct", "device-2");
        assert_eq!(
            store_genesis_intent(&without_digest_loc, &json!({"account": "acct"})),
            Err(GenesisEnableError::CorruptIntent)
        );
        Ok(())
    }
}
