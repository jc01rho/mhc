use super::bridge::{GenesisEnableError, GenesisEnableReason};
use crate::{
    fs::resilient,
    journal::{sync_journal_directory, sync_journal_file},
    locks::{AcquireLockOptions, WithLockError, create_lock_record, with_lock},
    support::{random::random_id, sha256::sha256_hex},
    sync::keyring_port::DeviceKeyring,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The public, exact signed request one device stores before it appends its genesis. `request` keeps
/// the wire shape of the append as it was signed; the chain walk that re-verifies it lives in
/// `intent-verify.ts`, outside this slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenesisIntent {
    pub version: u32,
    pub account: String,
    pub service_origin: Option<String>,
    pub service_device_id: String,
    pub sync_principal: IntentPrincipal,
    pub genesis_digest: String,
    pub request: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentPrincipal {
    pub kind: String,
    pub id: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}

pub struct IntentOwner<'a> {
    pub account: &'a str,
    pub service_origin: Option<&'a str>,
    pub service_device_id: &'a str,
    pub keyring: &'a dyn DeviceKeyring,
    pub pin_dir: &'a Path,
}

/// Where one account/principal keeps its genesis intent; its lock and phrase markers sit beside it.
pub fn genesis_intent_location(pin_dir: &Path, account: &str, principal_id: &str) -> PathBuf {
    let address = Value::Array(vec![account.into(), principal_id.into()]).to_string();
    pin_dir
        .join("genesis-intent")
        .join(sha256_hex(address.as_bytes()))
}

fn location(owner: &IntentOwner<'_>) -> PathBuf {
    genesis_intent_location(owner.pin_dir, owner.account, &owner.keyring.principal().id)
}

fn io(error: std::io::Error) -> GenesisEnableError {
    GenesisEnableError::with_cause(GenesisEnableReason::MissingConfiguration, error)
}

fn journal(error: crate::journal::JournalError) -> GenesisEnableError {
    GenesisEnableError::with_cause(
        GenesisEnableReason::MissingConfiguration,
        std::io::Error::other(error.to_string()),
    )
}

/// One account/principal ceremony across CLI and engine processes, including native preparation.
pub fn with_genesis_lock<T>(
    owner: &IntentOwner<'_>,
    run: impl FnOnce() -> Result<T, GenesisEnableError>,
) -> Result<T, GenesisEnableError> {
    let target = location(owner);
    let parent = target.parent().unwrap_or(&target).to_path_buf();
    resilient::create_dir_all(&parent).map_err(io)?;
    let record = create_lock_record("sync-genesis", Default::default()).map_err(|error| {
        GenesisEnableError::with_cause(GenesisEnableReason::MissingConfiguration, error)
    })?;
    with_lock(
        &with_suffix(&target, ".lock"),
        &record,
        &AcquireLockOptions::new(),
        run,
    )
    .map_err(|error| match error {
        WithLockError::User(error) => error,
        WithLockError::Acquire(error) => {
            GenesisEnableError::with_cause(GenesisEnableReason::MissingConfiguration, error)
        }
    })
}

pub fn load_genesis_intent(
    owner: &IntentOwner<'_>,
) -> Result<Option<GenesisIntent>, GenesisEnableError> {
    let target = location(owner);
    let raw = match resilient::read_to_string(&target.join("intent.json")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match resilient::metadata(&target) {
                Ok(_) => Err(GenesisEnableError::new(GenesisEnableReason::CorruptIntent)),
                Err(inner) if inner.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(inner) => Err(io(inner)),
            };
        }
        Err(error) => return Err(io(error)),
    };
    parse_genesis_intent(&raw, owner).map(Some)
}

/// The schema and identity checks of a stored intent: anything unreadable or about another device is
/// refused, never resumed.
pub fn parse_genesis_intent(
    raw: &str,
    owner: &IntentOwner<'_>,
) -> Result<GenesisIntent, GenesisEnableError> {
    let intent: GenesisIntent = serde_json::from_str(raw).map_err(|error| {
        GenesisEnableError::with_cause(GenesisEnableReason::CorruptIntent, error)
    })?;
    if intent.version != 1 || intent.genesis_digest.is_empty() || !intent.request.is_object() {
        return Err(GenesisEnableError::new(GenesisEnableReason::CorruptIntent));
    }
    let principal = owner.keyring.principal();
    let keys = owner.keyring.public_keys();
    let matches = intent.account == owner.account
        && intent.service_origin.as_deref() == owner.service_origin
        && intent.service_device_id == owner.service_device_id
        && intent.sync_principal.kind == principal.kind
        && intent.sync_principal.id == principal.id
        && intent.sync_principal.sealing_public_key == keys.sealing
        && intent.sync_principal.signing_public_key == keys.signing;
    if !matches {
        return Err(GenesisEnableError::new(GenesisEnableReason::IntentMismatch));
    }
    Ok(intent)
}

/// Public, exact signed request. Publishing a nonempty directory cannot overwrite another intent.
pub fn store_genesis_intent(
    owner: &IntentOwner<'_>,
    intent: &GenesisIntent,
) -> Result<(), GenesisEnableError> {
    let target = location(owner);
    let staging = with_suffix(
        &target,
        &format!(".{}.{}.tmp", std::process::id(), random_id()),
    );
    resilient::create_dir_all(&staging).map_err(io)?;
    let text = serde_json::to_string(intent).map_err(|error| {
        GenesisEnableError::with_cause(GenesisEnableReason::CorruptIntent, error)
    })?;
    let intent_file = staging.join("intent.json");
    let landed = (|| -> Result<(), GenesisEnableError> {
        resilient::write(&intent_file, text.as_bytes()).map_err(io)?;
        sync_journal_file(&intent_file).map_err(journal)?;
        sync_journal_directory(&staging).map_err(journal)?;
        resilient::rename(&staging, &target).map_err(io)
    })();
    if landed.is_err() {
        let _ = resilient::remove_dir_all(&staging);
    }
    landed?;
    sync_journal_directory(target.parent().unwrap_or(&target)).map_err(journal)?;
    match load_genesis_intent(owner)? {
        Some(written) if written.genesis_digest == intent.genesis_digest => Ok(()),
        _ => Err(GenesisEnableError::new(GenesisEnableReason::CorruptIntent)),
    }
}

pub fn retire_genesis_intent(owner: &IntentOwner<'_>) -> Result<(), GenesisEnableError> {
    let target = location(owner);
    let retired = with_suffix(&target, &format!(".retired-{}", random_id()));
    resilient::rename(&target, &retired).map_err(io)?;
    resilient::remove_dir_all(&retired).map_err(io)?;
    sync_journal_directory(target.parent().unwrap_or(&target)).map_err(journal)
}

fn with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
