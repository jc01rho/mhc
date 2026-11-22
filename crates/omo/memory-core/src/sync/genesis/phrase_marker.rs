//! The genesis phrase marker (A3 engine contract 5.1, review M1/L7): a local, non-secret record
//! that this device showed a recovery phrase for a genesis that may not have been saved. It holds
//! no words, no entropy and no key, only the account, when the ceremony began and, once built, the
//! genesis digest. Written durably before any word is shown, so a crash leaves a marker instead of
//! an unaccounted phrase.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs::resilient;
use crate::journal::sync_journal_directory;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhraseGenesisMarker {
    pub version: u8,
    pub account: String,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhraseGenesisPlace {
    /// The genesis intent location for this account and principal; the marker sits beside it.
    pub intent_location: PathBuf,
    pub account: String,
}

fn marker_path(place: &PhraseGenesisPlace) -> PathBuf {
    beside(place, ".phrase-pending")
}

fn beside(place: &PhraseGenesisPlace, suffix: &str) -> PathBuf {
    let base = place.intent_location.to_string_lossy();
    PathBuf::from(format!("{base}{suffix}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerRead {
    Absent,
    Present(PhraseGenesisMarker),
    Corrupt,
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    sync_journal_directory(dir).map_err(|error| std::io::Error::other(error.to_string()))
}

fn read_marker(place: &PhraseGenesisPlace) -> std::io::Result<MarkerRead> {
    let raw = match resilient::read_to_string(&marker_path(place)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MarkerRead::Absent);
        }
        Err(error) => return Err(error),
    };
    let Ok(marker) = serde_json::from_str::<PhraseGenesisMarker>(&raw) else {
        return Ok(MarkerRead::Corrupt);
    };
    if marker.version != 1 || marker.account != place.account {
        return Ok(MarkerRead::Corrupt);
    }
    Ok(MarkerRead::Present(marker))
}

/// Atomic and durable: a crash leaves the old marker or the new one, never a torn file.
pub fn write_phrase_genesis_marker(
    place: &PhraseGenesisPlace,
    marker: &PhraseGenesisMarker,
) -> std::io::Result<()> {
    let target = marker_path(place);
    let text = serde_json::to_vec(marker).map_err(std::io::Error::other)?;
    let staging = beside(
        place,
        &format!(
            ".phrase-pending.{}.tmp",
            crate::support::random::random_id()
        ),
    );
    if let Some(parent) = target.parent() {
        resilient::create_dir_all(parent)?;
    }
    let result = (|| -> std::io::Result<()> {
        let mut handle = resilient::create_exclusive(&staging)?;
        use std::io::Write as _;
        handle.write_all(&text)?;
        handle.sync_all()?;
        resilient::close_sync(handle);
        sync_dir(&staging)?;
        resilient::rename(&staging, &target)?;
        sync_dir(&target)
    })();
    let _ = resilient::remove_file(&staging);
    result
}

pub fn delete_phrase_genesis_marker(place: &PhraseGenesisPlace) -> std::io::Result<()> {
    match resilient::remove_file(&marker_path(place)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    sync_dir(&marker_path(place))
}

/// Whether a LIVE process holds this genesis lock (review M1): the lock file existing is not enough,
/// because a crashed owner leaves it behind. An owner this device cannot prove dead counts as live,
/// so a doubt reads as a running ceremony and never as a void phrase.
pub fn genesis_lock_held_by_live_owner(place: &PhraseGenesisPlace) -> std::io::Result<bool> {
    let lock = beside(place, ".lock");
    let raw = match resilient::read_to_string(&lock) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // An incomplete record counts as live: a doubt reads as a running ceremony.
    let Some(owner) = crate::locks::parse_lock_record(&raw) else {
        return Ok(true);
    };
    Ok(!crate::locks::is_lock_owner_proven_dead(&owner))
}

/// What the marker means for a phrase this device showed. `Void`: it was certainly not saved.
/// `Saved`: a stale marker whose genesis is this device's pinned root. `Unresolved`: unreadable
/// marker over an existing chain, never reported as void or saved. Rules (contract 5.1): no marker
/// or a pending intent means nothing to report; with no chain at all a shown phrase cannot have
/// landed; a recorded digest equal to the pinned root means the phrase genesis committed (the pin
/// is written before the intent is retired); otherwise the intent was never stored or was retired by
/// a lost race, so the phrase is void.
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
    pub chain_empty: bool,
}

pub fn genesis_phrase_state(
    place: &PhraseGenesisPlace,
    facts: &GenesisPhraseFacts<'_>,
) -> std::io::Result<GenesisPhraseState> {
    let read = read_marker(place)?;
    if matches!(read, MarkerRead::Absent) || facts.intent_present {
        return Ok(GenesisPhraseState::None);
    }
    if facts.chain_empty {
        return Ok(GenesisPhraseState::Void);
    }
    let MarkerRead::Present(marker) = read else {
        return Ok(GenesisPhraseState::Unresolved);
    };
    Ok(match marker.genesis_digest {
        Some(digest) if Some(digest.as_str()) == facts.pinned_genesis_digest => {
            GenesisPhraseState::Saved
        }
        _ => GenesisPhraseState::Void,
    })
}

/// Whether an intent file exists, without parsing it; status reads never need the keyring.
pub fn genesis_intent_present(place: &PhraseGenesisPlace) -> std::io::Result<bool> {
    match resilient::metadata(&place.intent_location.join("intent.json")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(dir: &Path) -> PhraseGenesisPlace {
        PhraseGenesisPlace {
            intent_location: dir.join("genesis-intent").join("abc123"),
            account: "acct".into(),
        }
    }

    fn marker(digest: Option<&str>) -> PhraseGenesisMarker {
        PhraseGenesisMarker {
            version: 1,
            account: "acct".into(),
            started_at: 1_700_000_000_000,
            genesis_digest: digest.map(str::to_string),
        }
    }

    fn facts(
        intent_present: bool,
        pinned: Option<&str>,
        chain_empty: bool,
    ) -> GenesisPhraseFacts<'_> {
        GenesisPhraseFacts {
            intent_present,
            pinned_genesis_digest: pinned,
            chain_empty,
        }
    }

    #[test]
    fn a_shown_phrase_without_marker_or_with_a_pending_intent_reports_nothing()
    -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let place = place(dir.path());
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, None, true))?,
            GenesisPhraseState::None
        );
        write_phrase_genesis_marker(&place, &marker(None))?;
        assert_eq!(
            genesis_phrase_state(&place, &facts(true, None, true))?,
            GenesisPhraseState::None
        );
        Ok(())
    }

    #[test]
    fn the_marker_is_never_words_and_its_states_follow_the_contract() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let place = place(dir.path());
        write_phrase_genesis_marker(&place, &marker(Some("root-1")))?;
        let written = resilient::read_to_string(&marker_path(&place))?;
        assert!(!written.contains("abandon"), "{written}");
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, Some("root-1"), false))?,
            GenesisPhraseState::Saved
        );
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, Some("root-1"), true))?,
            GenesisPhraseState::Void
        );
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, Some("other-root"), false))?,
            GenesisPhraseState::Void
        );
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, None, false))?,
            GenesisPhraseState::Void
        );
        delete_phrase_genesis_marker(&place)?;
        assert!(!marker_path(&place).exists());
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, None, false))?,
            GenesisPhraseState::None
        );
        assert!(!genesis_intent_present(&place)?);

        write_phrase_genesis_marker(&place, &marker(None))?;
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, None, true))?,
            GenesisPhraseState::Void
        );
        Ok(())
    }

    #[test]
    fn genesis_lock_liveness_treats_corrupt_lock_as_live_and_absent_as_free() -> std::io::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let place = place(dir.path());
        assert!(!genesis_lock_held_by_live_owner(&place)?);
        resilient::create_dir_all(&dir.path().join("genesis-intent"))?;
        resilient::write(&beside(&place, ".lock"), b"corrupt-lock")?;
        assert!(genesis_lock_held_by_live_owner(&place)?);
        let live = crate::locks::create_lock_record(
            "sync-genesis",
            crate::locks::CreateLockRecordOptions::default(),
        )
        .expect("lock record");
        resilient::write(
            &beside(&place, ".lock"),
            serde_json::to_vec(&live).expect("json").as_slice(),
        )?;
        assert!(genesis_lock_held_by_live_owner(&place)?);
        Ok(())
    }

    #[test]
    fn an_unreadable_or_foreign_marker_is_unresolved_over_a_chain() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let place = place(dir.path());
        write_phrase_genesis_marker(&place, &marker(Some("root-1")))?;
        resilient::write(&marker_path(&place), b"not json")?;
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, Some("root-1"), false))?,
            GenesisPhraseState::Unresolved
        );
        let mut foreign = marker(Some("root-1"));
        foreign.account = "someone-else".into();
        write_phrase_genesis_marker(&place, &foreign)?;
        assert_eq!(
            genesis_phrase_state(&place, &facts(false, Some("root-1"), false))?,
            GenesisPhraseState::Unresolved
        );
        Ok(())
    }
}
