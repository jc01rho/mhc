use super::{bridge::GenesisEnableError, intent_store::genesis_intent_location};
use crate::{
    fs::resilient,
    locks::{is_lock_owner_proven_dead, parse_lock_record},
    sync::engine::atomic::{remove_atomic, write_atomic},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The genesis phrase marker (A3 engine contract 5.1): a local, non-secret record that this device
/// showed a recovery phrase for a genesis that may not have been saved. It holds no words, no entropy
/// and no key, only the account, when the ceremony began and, once built, the genesis digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PhraseGenesisMarker {
    pub version: u32,
    pub account: String,
    pub started_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhraseGenesisPlace {
    pub pin_dir: PathBuf,
    pub account: String,
    pub principal_id: String,
}

fn with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn location(place: &PhraseGenesisPlace) -> PathBuf {
    genesis_intent_location(&place.pin_dir, &place.account, &place.principal_id)
}

fn marker_path(place: &PhraseGenesisPlace) -> PathBuf {
    with_suffix(&location(place), ".phrase-pending")
}

/// The marker file of this place; the tests corrupt it directly to prove the unresolved state.
pub fn phrase_genesis_marker_path(place: &PhraseGenesisPlace) -> PathBuf {
    marker_path(place)
}

fn lock_path(place: &PhraseGenesisPlace) -> PathBuf {
    with_suffix(&location(place), ".lock")
}

fn intent_file(place: &PhraseGenesisPlace) -> PathBuf {
    location(place).join("intent.json")
}

fn io(error: std::io::Error) -> GenesisEnableError {
    GenesisEnableError::with_cause(
        super::bridge::GenesisEnableReason::MissingConfiguration,
        error,
    )
}

fn engine(error: crate::sync::engine::error::EngineError) -> GenesisEnableError {
    GenesisEnableError::with_cause(
        super::bridge::GenesisEnableReason::CorruptIntent,
        std::io::Error::other(error.to_string()),
    )
}

enum MarkerRead {
    Absent,
    Present(PhraseGenesisMarker),
    Corrupt,
}

fn read_marker(place: &PhraseGenesisPlace) -> Result<MarkerRead, GenesisEnableError> {
    let raw = match resilient::read_to_string(&marker_path(place)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MarkerRead::Absent);
        }
        Err(error) => return Err(io(error)),
    };
    let Ok(marker) = serde_json::from_str::<PhraseGenesisMarker>(&raw) else {
        return Ok(MarkerRead::Corrupt);
    };
    if marker.version != 1 || marker.account != place.account || !marker.started_at.is_finite() {
        return Ok(MarkerRead::Corrupt);
    }
    Ok(MarkerRead::Present(marker))
}

/// Atomic and durable: a crash leaves the old marker or the new one, never a torn file.
pub fn write_phrase_genesis_marker(
    place: &PhraseGenesisPlace,
    marker: &PhraseGenesisMarker,
) -> Result<(), GenesisEnableError> {
    let text = serde_json::to_string(marker).map_err(|error| {
        GenesisEnableError::with_cause(super::bridge::GenesisEnableReason::CorruptIntent, error)
    })?;
    write_atomic(&marker_path(place), text.as_bytes()).map_err(engine)
}

pub fn delete_phrase_genesis_marker(place: &PhraseGenesisPlace) -> Result<(), GenesisEnableError> {
    remove_atomic(&marker_path(place)).map_err(engine)
}

/// Whether a LIVE process holds this genesis lock: the lock file existing is not enough, because a
/// crashed owner leaves it behind. The owner is dead only on the lock code's own proof; an incomplete
/// record, another host or unknown liveness all count as live, so a doubt reads as a running ceremony
/// and never as a void phrase.
pub fn genesis_lock_held_by_live_owner(place: &PhraseGenesisPlace) -> bool {
    match resilient::read_to_string(&lock_path(place)) {
        Ok(raw) => parse_lock_record(&raw).is_none_or(|owner| !is_lock_owner_proven_dead(&owner)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// What the marker means for a phrase this device showed. `Void`: it was certainly not saved.
/// `Saved`: a stale marker whose genesis is this device's pinned root. `Unresolved`: an unreadable
/// marker over an existing chain, never reported as void or saved. Rules (contract 5.1): no marker or
/// a pending intent means nothing to report; with no chain at all a shown phrase cannot have landed; a
/// recorded digest equal to the pinned root means the phrase genesis committed (the pin is written
/// before the intent is retired); otherwise the intent was never stored or was retired by a lost race,
/// so the phrase is void.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisPhraseState {
    None,
    Void,
    Saved,
    Unresolved,
}

pub struct GenesisPhraseFacts<'a> {
    pub intent_present: bool,
    pub pinned_genesis_digest: Option<&'a str>,
    pub chain_empty: Option<bool>,
}

pub fn genesis_phrase_state(
    place: &PhraseGenesisPlace,
    facts: &GenesisPhraseFacts<'_>,
) -> Result<GenesisPhraseState, GenesisEnableError> {
    let read = read_marker(place)?;
    if matches!(read, MarkerRead::Absent) || facts.intent_present {
        return Ok(GenesisPhraseState::None);
    }
    if facts.chain_empty == Some(true) {
        return Ok(GenesisPhraseState::Void);
    }
    let MarkerRead::Present(marker) = read else {
        return Ok(GenesisPhraseState::Unresolved);
    };
    match marker.genesis_digest.as_deref() {
        Some(digest) if Some(digest) == facts.pinned_genesis_digest => {
            Ok(GenesisPhraseState::Saved)
        }
        _ => Ok(GenesisPhraseState::Void),
    }
}

/// Whether an intent file exists, without parsing it; status reads never need the keyring.
pub fn genesis_intent_present(place: &PhraseGenesisPlace) -> Result<bool, GenesisEnableError> {
    match resilient::metadata(&intent_file(place)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io(error)),
    }
}
